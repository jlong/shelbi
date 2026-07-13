//! Version handshake on the hub socket.
//!
//! Root cause of the shaft-project outage: Homebrew upgrades swap the
//! `shelbi` binary but never restart the long-lived hub daemon, so an
//! old daemon keeps writing state in its old shape while new CLI
//! one-shots read the new layout — surfacing as undiagnosable, path-less
//! `io: No such file or directory` failures deep in the transition path.
//!
//! The handshake closes that gap without breaking already-running clients
//! from the previous binary. A probe opens a dedicated connection and
//! immediately half-closes its write side. A handshake-aware daemon treats
//! EOF-before-any-frame as a hello request and replies with one
//! newline-terminated JSON frame:
//!
//! ```text
//! {"hello":"shelbi-daemon","version":"0.4.0","protocol":1}
//! ```
//!
//! Ordinary event/message connections are deliberately unchanged: their
//! first response remains the post-dispatch `ok\n` acknowledgement. That
//! ordering is load-bearing for pre-handshake pane wrappers, which read
//! exactly those three bytes before deciding whether to retry and fall back
//! to a direct append.
//!
//! Clients compare `version` against their own `CARGO_PKG_VERSION`
//! (exact match — daemon and CLI ship from the same workspace version)
//! and `protocol` against [`HUB_PROTOCOL_VERSION`]. A daemon that sends
//! *no* hello (the pre-handshake generations — exactly the 0.1 case) closes
//! the empty connection without a response and is treated as a mismatch,
//! not an error loop.

use std::io::{BufRead, BufReader, Read};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::hub_socket_path;

/// Socket protocol number. Bump on any incompatible change to the hub
/// socket's frame format so a newer/older peer degrades to a clean
/// version-mismatch error instead of misparsing frames.
pub const HUB_PROTOCOL_VERSION: u32 = 1;

/// This workspace build's version. Every Shelbi crate ships with one exact
/// workspace version, so TUI and CLI mutation paths can share the same
/// compatibility decision without range negotiation.
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The `hello` discriminator value the daemon stamps on its hello frame.
/// Parsing requires it verbatim, so an unrelated JSON line (or a future
/// different speaker on the socket) can't be mistaken for a daemon hello.
const HELLO_DISCRIMINATOR: &str = "shelbi-daemon";

/// How long a probing client waits for the daemon's hello before
/// concluding it's a pre-handshake daemon. A live post-handshake daemon
/// replies as soon as it reads the probe's EOF, so a healthy round-trip is
/// sub-millisecond; the full second is only paid for a confused listener
/// that neither answers nor closes.
const HELLO_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Upper bound on the hello line a client will read. A real hello is
/// ~60 bytes; anything past this is not a hello and the read stops
/// rather than buffering unbounded garbage from a confused peer.
const MAX_HELLO_LINE_BYTES: u64 = 4096;

/// The hello frame the daemon writes in response to an empty, half-closed
/// probe connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHello {
    /// Always [`HELLO_DISCRIMINATOR`] — lets parsers reject non-hello
    /// JSON lines outright.
    pub hello: String,
    /// The daemon binary's semver (`CARGO_PKG_VERSION` at build time).
    pub version: String,
    /// The daemon's [`HUB_PROTOCOL_VERSION`].
    pub protocol: u32,
}

impl DaemonHello {
    pub fn new(version: &str) -> Self {
        Self {
            hello: HELLO_DISCRIMINATOR.to_string(),
            version: version.to_string(),
            protocol: HUB_PROTOCOL_VERSION,
        }
    }

    /// Serialize to the newline-terminated wire line the daemon writes.
    pub fn to_line(&self) -> String {
        // A struct of two strings and a u32 can't fail to serialize;
        // fall back to an empty line rather than panicking in the
        // daemon's accept path if that ever changes.
        let mut line = serde_json::to_string(self).unwrap_or_default();
        line.push('\n');
        line
    }

    /// Parse one line as a hello frame. `None` for anything that isn't
    /// one — malformed JSON, a different discriminator, a missing field.
    pub fn parse(line: &str) -> Option<Self> {
        let hello: DaemonHello = serde_json::from_str(line.trim()).ok()?;
        (hello.hello == HELLO_DISCRIMINATOR).then_some(hello)
    }
}

/// Outcome of probing the hub socket for the daemon's hello frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonProbe {
    /// Connecting to the hub socket failed — no daemon is listening.
    NotRunning,
    /// A daemon accepted the connection but sent no parseable hello
    /// within the timeout: a pre-handshake daemon (0.3.x or older).
    NoHello,
    /// A post-handshake daemon answered with its hello frame.
    Hello(DaemonHello),
}

/// Outcome of comparing a daemon probe with a client build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonVersionStatus {
    /// Nothing is listening on the hub socket.
    NotRunning,
    /// Daemon and client agree on exact version and socket protocol.
    Match { version: String },
    /// A live daemon is incompatible. `daemon` is a human-readable
    /// description suitable for the CLI, TUI, and background logs.
    Mismatch { daemon: String },
}

/// Connect to the hub socket and read the daemon's hello frame.
///
/// The probe sends no frame. Instead it half-closes its write side immediately:
/// a post-handshake daemon interprets EOF-before-any-frame as the hello request,
/// while a pre-handshake daemon simply closes the connection. This client-first
/// negotiation preserves the exact legacy `ok\n` response on real event
/// connections and therefore remains safe for pane wrappers that survived a
/// daemon restart across an upgrade.
pub fn probe_daemon_hello() -> DaemonProbe {
    let Ok(sock) = hub_socket_path() else {
        return DaemonProbe::NotRunning;
    };
    let Ok(stream) = UnixStream::connect(&sock) else {
        return DaemonProbe::NotRunning;
    };
    let _ = stream.set_read_timeout(Some(HELLO_READ_TIMEOUT));
    // EOF-without-a-frame is the negotiation request. Ignore a shutdown error:
    // the bounded read below still classifies a peer that cannot answer as
    // NoHello rather than turning a stale daemon into an error loop.
    let _ = stream.shutdown(Shutdown::Write);
    match read_hello_line(&stream) {
        Some(hello) => DaemonProbe::Hello(hello),
        None => DaemonProbe::NoHello,
    }
}

/// Classify an already-completed probe against `client_version`. Kept pure so
/// CLI/TUI formatting and protocol-skew tests all consume one decision rule.
pub fn classify_daemon_version(
    probe: DaemonProbe,
    client_version: &str,
) -> DaemonVersionStatus {
    match probe {
        DaemonProbe::NotRunning => DaemonVersionStatus::NotRunning,
        DaemonProbe::NoHello => DaemonVersionStatus::Mismatch {
            daemon: "an older version (predates the version handshake)".into(),
        },
        DaemonProbe::Hello(h) if h.protocol != HUB_PROTOCOL_VERSION => {
            DaemonVersionStatus::Mismatch {
                daemon: format!(
                    "{} (socket protocol {}, this CLI speaks {})",
                    h.version, h.protocol, HUB_PROTOCOL_VERSION
                ),
            }
        }
        DaemonProbe::Hello(h) if h.version != client_version => {
            DaemonVersionStatus::Mismatch { daemon: h.version }
        }
        DaemonProbe::Hello(h) => DaemonVersionStatus::Match { version: h.version },
    }
}

/// Probe and compare the daemon with this workspace build.
pub fn daemon_version_status() -> DaemonVersionStatus {
    classify_daemon_version(probe_daemon_hello(), CLIENT_VERSION)
}

/// Non-interactive guard for mutation paths outside the top-level CLI (TUI,
/// palette internals, poller, and state helpers). A missing daemon remains
/// allowed because these paths have always supported direct-file fallback;
/// only a live incompatible daemon is refused.
pub fn ensure_daemon_matches_for_mutation() -> shelbi_core::Result<()> {
    let DaemonVersionStatus::Mismatch { daemon } = daemon_version_status() else {
        return Ok(());
    };
    Err(shelbi_core::Error::Other(format!(
        "hub daemon is {daemon}, CLI is {CLIENT_VERSION} — run `shelbi daemon restart` to put \
         the daemon on the current binary"
    )))
}

/// Read and parse one bounded line as a hello frame. `None` on timeout,
/// EOF, over-length, or a line that isn't a hello.
fn read_hello_line(stream: &UnixStream) -> Option<DaemonHello> {
    let reader = BufReader::new(stream);
    let mut buf = Vec::with_capacity(128);
    let n = reader
        .take(MAX_HELLO_LINE_BYTES)
        .read_until(b'\n', &mut buf)
        .ok()?;
    if n == 0 {
        return None;
    }
    DaemonHello::parse(std::str::from_utf8(&buf).ok()?)
}

/// Read the daemon's per-line [`DAEMON_ACK`](crate::DAEMON_ACK) off
/// `stream`, tolerating (and discarding) the leading hello frame emitted by
/// the briefly-shipped server-first implementation. Stable post-handshake and
/// pre-handshake daemons both send the ack as the first line; retaining this
/// tolerance lets a newly upgraded client still talk safely to that interim
/// daemon long enough to detect and restart it.
///
/// Reads at most two bounded lines; anything else is an error so a
/// caller's file fallback fires rather than mistaking garbage for
/// delivery.
pub fn read_daemon_ack<R: Read>(stream: R) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    for _ in 0..2 {
        let mut buf = Vec::with_capacity(64);
        let n = (&mut reader)
            .take(MAX_HELLO_LINE_BYTES)
            .read_until(b'\n', &mut buf)?;
        if n == 0 {
            break; // EOF before an ack
        }
        if buf == crate::DAEMON_ACK {
            return Ok(());
        }
        if DaemonHello::parse(std::str::from_utf8(&buf).unwrap_or("")).is_some() {
            continue; // hello frame — the ack is the next line
        }
        break; // neither hello nor ack — don't keep reading
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "no daemon ack",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::Shutdown;

    #[test]
    fn hello_round_trips_through_the_wire_line() {
        let hello = DaemonHello::new("0.4.0");
        let line = hello.to_line();
        assert!(line.ends_with('\n'), "wire line must be newline-terminated");
        let parsed = DaemonHello::parse(&line).expect("round-trip parse");
        assert_eq!(parsed, hello);
        assert_eq!(parsed.protocol, HUB_PROTOCOL_VERSION);
    }

    #[test]
    fn parse_rejects_non_hello_lines() {
        for line in [
            "not json",
            "{}",
            r#"{"hello":"someone-else","version":"0.4.0","protocol":1}"#,
            r#"{"verb":"event","line":"x=1"}"#,
            "ok",
            "",
        ] {
            assert!(DaemonHello::parse(line).is_none(), "accepted: {line:?}");
        }
    }

    #[test]
    fn read_daemon_ack_accepts_bare_ack_from_pre_handshake_daemon() {
        read_daemon_ack(&b"ok\n"[..]).expect("bare ack");
    }

    #[test]
    fn read_daemon_ack_skips_leading_hello() {
        let wire = format!("{}ok\n", DaemonHello::new("0.4.0").to_line());
        read_daemon_ack(wire.as_bytes()).expect("hello then ack");
    }

    #[test]
    fn read_daemon_ack_errors_on_hello_without_ack() {
        let wire = DaemonHello::new("0.4.0").to_line();
        assert!(read_daemon_ack(wire.as_bytes()).is_err());
    }

    #[test]
    fn read_daemon_ack_errors_on_garbage() {
        assert!(read_daemon_ack(&b"nope\n"[..]).is_err());
        assert!(read_daemon_ack(&b""[..]).is_err());
    }

    #[test]
    fn probe_reports_not_running_without_a_daemon() {
        let _lock = crate::test_lock::LOCK.lock().unwrap();
        let dir = std::env::temp_dir().join(format!(
            "shelbi-hello-probe-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var("SHELBI_HUB_SOCK").ok();
        std::env::set_var("SHELBI_HUB_SOCK", dir.join("hub.sock"));
        assert_eq!(probe_daemon_hello(), DaemonProbe::NotRunning);
        match prev {
            Some(v) => std::env::set_var("SHELBI_HUB_SOCK", v),
            None => std::env::remove_var("SHELBI_HUB_SOCK"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn probe_classifies_hello_and_silent_listeners() {
        use std::os::unix::net::UnixListener;
        let _lock = crate::test_lock::LOCK.lock().unwrap();
        // macOS caps Unix-socket paths at ~104 bytes; keep it short.
        let sock = std::path::PathBuf::from(format!("/tmp/shb-hello-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let prev = std::env::var("SHELBI_HUB_SOCK").ok();
        std::env::set_var("SHELBI_HUB_SOCK", &sock);

        // A post-handshake daemon: waits for the empty connection's EOF,
        // then answers with the hello. A pre-handshake daemon reads the same
        // EOF and closes without writing.
        let server = std::thread::spawn(move || {
            let (mut s1, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            s1.read_to_end(&mut request).unwrap();
            assert!(request.is_empty());
            s1.write_all(DaemonHello::new("9.9.9").to_line().as_bytes())
                .unwrap();
            let _ = s1.shutdown(Shutdown::Both);
            let (mut s2, _) = listener.accept().unwrap();
            request.clear();
            s2.read_to_end(&mut request).unwrap();
            assert!(request.is_empty());
            drop(s2);
        });

        match probe_daemon_hello() {
            DaemonProbe::Hello(h) => {
                assert_eq!(h.version, "9.9.9");
                assert_eq!(h.protocol, HUB_PROTOCOL_VERSION);
            }
            other => panic!("expected Hello, got {other:?}"),
        }
        assert_eq!(
            probe_daemon_hello(),
            DaemonProbe::NoHello,
            "a silent listener is a pre-handshake daemon"
        );

        server.join().unwrap();
        match prev {
            Some(v) => std::env::set_var("SHELBI_HUB_SOCK", v),
            None => std::env::remove_var("SHELBI_HUB_SOCK"),
        }
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn version_classification_covers_semver_protocol_and_pre_handshake() {
        assert_eq!(
            classify_daemon_version(DaemonProbe::NotRunning, "1.2.3"),
            DaemonVersionStatus::NotRunning
        );
        assert_eq!(
            classify_daemon_version(
                DaemonProbe::Hello(DaemonHello::new("1.2.3")),
                "1.2.3"
            ),
            DaemonVersionStatus::Match {
                version: "1.2.3".into()
            }
        );
        assert_eq!(
            classify_daemon_version(
                DaemonProbe::Hello(DaemonHello::new("0.1.0")),
                "1.2.3"
            ),
            DaemonVersionStatus::Mismatch {
                daemon: "0.1.0".into()
            }
        );
        let mut protocol = DaemonHello::new("1.2.3");
        protocol.protocol += 1;
        let DaemonVersionStatus::Mismatch { daemon } =
            classify_daemon_version(DaemonProbe::Hello(protocol), "1.2.3")
        else {
            panic!("protocol skew must mismatch");
        };
        assert!(daemon.contains("socket protocol"), "{daemon}");
        let DaemonVersionStatus::Mismatch { daemon } =
            classify_daemon_version(DaemonProbe::NoHello, "1.2.3")
        else {
            panic!("pre-handshake daemon must mismatch");
        };
        assert!(daemon.contains("older"), "{daemon}");
    }
}
