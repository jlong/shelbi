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
//! The link exposes nothing on either machine: no forwarded ports, no socket
//! file beyond the shared connection's own in Shelbi's `0700` ssh directory
//! ([`super::ssh::remote_control_path`], which every connect step already
//! uses), and this machine's `hub.sock` never reaches the far host. It never
//! starts the far daemon; a service that needs it fails to open while it is
//! stopped.
//!
//! The `ssh` rides the shared connection the connect steps signed in, so a
//! host that only takes a password doesn't ask twice. When the sign-in comes
//! back refused while reconnecting (a wrong password, a Cancel), the host link
//! holds that answer rather than ask again every few seconds, until the user
//! says to try again ([`HostLink::allow_sign_in`]).
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

use shelbi_ssh::askpass::{wait_for_output, Askpass, PromptWatch};

use super::remote::{validate_host, RemoteError};
use super::ssh::{classify_ssh_failure, relay_command, remote_control_path, SshAuth, SshOutput, STEP_DEADLINE};
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
    /// relay's hello, on the shared connection, asking through the dialog
    /// when it must ([`SshAuth::detect`]). Fails when `ssh` can't sign in or
    /// reach the host, or the host has no Shelbi, with `ssh`'s own last line
    /// as the reason.
    pub fn open(host: &str) -> Result<RemoteLink, RemoteError> {
        let host = validate_host(host)?;
        let auth = SshAuth::detect().for_host(&host);
        let argv = relay_command(&host, &auth, remote_control_path().as_deref());
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        RemoteLink::spawn_asking(&host, cmd, auth.askpass())
    }

    /// Open a link over `cmd`, a command whose stdio speaks the relay
    /// protocol. [`RemoteLink::open`] passes the `ssh` from
    /// [`relay_command`]; tests pass a local `shelbi relay`.
    pub fn spawn(host: &str, cmd: Command) -> Result<RemoteLink, RemoteError> {
        RemoteLink::spawn_asking(host, cmd, None)
    }

    /// [`spawn`](Self::spawn) with `cmd` an `ssh` that may ask through
    /// `askpass`: a dialog that is open doesn't count against the wait for the
    /// hello, and a failure says whether ssh could have asked.
    pub fn spawn_asking(host: &str, mut cmd: Command, askpass: Option<&Askpass>) -> Result<RemoteLink, RemoteError> {
        let mut watch = askpass.and_then(|askpass| {
            askpass.apply(&mut cmd);
            let watch = PromptWatch::new().ok()?;
            watch.apply(&mut cmd);
            Some(watch)
        });
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| RemoteError::Unreachable {
                host: host.to_string(),
                reason: format!("couldn't run ssh: {e}"),
            })?;
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
        // No channel yet: its keepalive counts silence as a dead link, and
        // the far side can't answer until the user has signed in. Whatever
        // comes first (the hello, or ssh giving up) ends this wait.
        let answered = wait_for_output(&stdout, watch.as_mut(), STEP_DEADLINE);
        drop(watch);
        let hello = answered.map_err(|e| e.to_string()).and_then(|()| {
            // The relay says hello first thing; no hello (or a channel that
            // can't even write, ssh having exited) means ssh or the far side
            // failed, and ssh has said why on stderr.
            let no_relay = |_| "the relay didn't start".to_string();
            let channel = RelayChannel::new(Box::new(stdout), Box::new(stdin)).map_err(no_relay)?;
            let version = channel.peer_version(STEP_DEADLINE).map_err(no_relay)?;
            Ok((channel, version))
        });
        let (channel, relay_version) = match hello {
            Ok(up) => up,
            Err(why) => {
                let code = end(&mut child);
                // Let the stderr reader catch ssh's last words.
                std::thread::sleep(Duration::from_millis(50));
                let said = stderr.lock().unwrap().clone();
                let reason = said.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or(&why).to_string();
                let out = SshOutput {
                    code,
                    stderr: said,
                    asked: askpass.is_some(),
                    ..Default::default()
                };
                return Err(classify_ssh_failure(host, &out).unwrap_or_else(|| RemoteError::Unreachable {
                    host: host.to_string(),
                    reason,
                }));
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
/// once share one `ssh` rather than each starting their own. A sign-in the
/// user was asked for and that was refused is held longer: see
/// [`HostLink::allow_sign_in`].
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
    /// [`REOPEN_PAUSE`] after a failure, they get that failure back, and after
    /// a refused sign-in they get it back until
    /// [`allow_sign_in`](Self::allow_sign_in).
    pub fn connect(&self, check: &dyn Fn() -> Result<(), RemoteError>) -> Result<Arc<dyn RelayLink>, RemoteError> {
        if let Some(link) = self.live() {
            return Ok(link);
        }
        let mut opening = self.opening.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(link) = self.live() {
            return Ok(link);
        }
        if let Some((at, error)) = opening.as_ref() {
            if at.elapsed() < self.pause || sign_in_refused(error) {
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

    /// The user wants this host again (they opened it, or chose to sign in):
    /// the next [`connect`](Self::connect) may ask them, whatever they
    /// answered last. Waits out a dialog that is open, so call it off the UI
    /// thread.
    pub fn allow_sign_in(&self) {
        let mut opening = self.opening.lock().unwrap_or_else(|p| p.into_inner());
        if opening.as_ref().is_some_and(|(_, error)| sign_in_refused(error)) {
            *opening = None;
        }
    }

    fn channel(&self) -> Result<Arc<dyn RelayLink>, ClientError> {
        self.live().ok_or(ClientError::RelayUnreachable)
    }
}

/// Whether `error` is the user's own answer: ssh could ask, and the sign-in
/// was refused (a wrong password, a Cancel). A reconnect that retried on its
/// own would put the dialog back up every few seconds.
pub fn sign_in_refused(error: &RemoteError) -> bool {
    matches!(error, RemoteError::AuthFailed { asked: true, .. })
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

/// End a child this link started and reap it: its exit code, if it had one
/// of its own before this.
fn end(child: &mut Child) -> Option<i32> {
    let _ = child.kill();
    child.wait().ok()?.code()
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

    fn refused_sign_in(asked: bool) -> RemoteError {
        RemoteError::AuthFailed {
            host: "alex@studio".into(),
            reason: "Permission denied (publickey,password).".into(),
            asked,
        }
    }

    /// Connect with a check that answers `result`, counting its runs (each
    /// real check would put the dialog up).
    fn counting_checks(link: &HostLink, result: impl Fn() -> Result<(), RemoteError>) -> (Result<(), RemoteError>, u64) {
        let checks = AtomicU64::new(0);
        let opened = link.connect(&|| {
            checks.fetch_add(1, Ordering::SeqCst);
            result()
        });
        (opened.map(|_| ()), checks.load(Ordering::SeqCst))
    }

    #[test]
    fn a_refused_sign_in_isnt_asked_again_until_the_user_says() {
        let (link, _) = fake_link();
        let refused = || Err(refused_sign_in(true));
        assert_eq!(counting_checks(&link, refused), (Err(refused_sign_in(true)), 1));
        for _ in 0..3 {
            // The window's watch and each pane, with no pause configured.
            assert_eq!(counting_checks(&link, refused), (Err(refused_sign_in(true)), 0), "no dialog");
        }
        assert_eq!(link.opens(), 0);

        link.allow_sign_in();
        assert_eq!(counting_checks(&link, || Ok(())), (Ok(()), 1), "asked once, and in");
        assert!(link.is_up());
        assert_eq!(link.opens(), 1);
    }

    #[test]
    fn a_sign_in_that_couldnt_ask_keeps_trying_as_ever() {
        // Nobody was asked (no dialog here), so the agent may yet have a key.
        let (link, _) = fake_link();
        let refused = || Err(refused_sign_in(false));
        assert_eq!(counting_checks(&link, refused).1, 1);
        assert_eq!(counting_checks(&link, refused).1, 1);
        assert_eq!(counting_checks(&link, || Ok(())), (Ok(()), 1));
    }

    #[test]
    fn allowing_a_sign_in_leaves_other_failures_paused() {
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
        assert!(link.connect(&ok).is_err());
        link.allow_sign_in();
        assert!(link.connect(&ok).is_err());
        assert_eq!(tries.load(Ordering::SeqCst), 1);
    }

    /// `sh -c script`, standing in for `ssh`.
    fn stand_in(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    #[test]
    fn a_refused_relay_sign_in_is_a_sign_in_failure_not_unreachable() {
        const DENIED: &str = "echo 'alex@studio: Permission denied (publickey,password,keyboard-interactive).' >&2; exit 255";
        let askpass = Askpass::new("/bin/false");
        let err = RemoteLink::spawn_asking("alex@studio", stand_in(DENIED), Some(&askpass)).err().unwrap();
        assert_eq!(
            err,
            RemoteError::AuthFailed {
                host: "alex@studio".into(),
                reason: "alex@studio: Permission denied (publickey,password,keyboard-interactive).".into(),
                asked: true,
            }
        );
        // With no dialog possible, the same refusal says Shelbi couldn't ask.
        let err = RemoteLink::spawn("alex@studio", stand_in(DENIED)).err().unwrap();
        assert!(matches!(err, RemoteError::AuthFailed { asked: false, .. }), "{err:?}");
    }

    #[test]
    fn a_relay_that_asks_runs_with_the_askpass_environment() {
        // The stand-in says what it was given, as ssh's stderr would, and exits.
        let script = "echo \"Permission denied $SSH_ASKPASS $SSH_ASKPASS_REQUIRE $SHELBI_ASKPASS_HOST\" >&2; exit 255";
        let askpass = Askpass::new("/bin/false").with_host("alex@studio");
        let err = RemoteLink::spawn_asking("alex@studio", stand_in(script), Some(&askpass)).err().unwrap();
        let RemoteError::AuthFailed { reason, .. } = err else {
            panic!("expected a sign-in failure");
        };
        assert_eq!(reason, "Permission denied /bin/false force alex@studio");
    }

    #[test]
    fn a_relay_that_says_nothing_is_unreachable() {
        let err = RemoteLink::spawn("devbox", stand_in("exit 3")).err().unwrap();
        assert_eq!(
            err,
            RemoteError::Unreachable {
                host: "devbox".into(),
                reason: "the relay didn't start".into(),
            }
        );
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
